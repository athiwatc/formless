//! `formless-desktop`: optional local daemon client or Phase 0 fixture shell.
//!
//! `--connect` contacts an existing daemon; it never starts or configures one.
//!
//! Options:
//! - `--connect PATH`: use a local daemon socket or full Windows pipe name.
//! - `--start-daemon`: start/reuse the profile daemon via the CLI, then connect.
//!   This is implicit inside a macOS app bundle without fixture/diagnostic flags.
//!   `--connect` wins; startup failures show an alert, then quit.
//! - `--sessions N`, `--blocks N`: size the fixture.
//! - `--huge-block-mb N`: add one N MiB output block at the tail.
//! - `--text-scale P`: override saved text size (100 to 200) for this launch.
//! - `--preferences PATH`: use this explicit desktop view preference file.
//! - `--no-preferences`: disable preference reads/writes. Fixture diagnostics
//!   default to disabled; daemon mode defaults to the user's desktop config path.
//! - `--window WxH`: open at this size instead of 1440x900.
//! - `--keep-window`: do not ask the window to grow to fit the text size, so
//!   a small window shows the reflowed shell. With `--smoke`, focus is then
//!   moved to each part of the page in turn and where the page scrolled to
//!   is printed.
//! - `--background`: do not bring the application to the front when it
//!   opens, so whatever the user is typing into keeps the keyboard.
//! - `--lifetime S`: quit by itself after S seconds. With `--background`,
//!   this is for inspecting the window from outside without leaving it open.
//! - `--dump-a11y PATH`: every half second, write to PATH the accessibility
//!   tree GPUI last handed to the platform's adapter, as JSON. GPUI only
//!   builds that tree once something has asked the platform for it (a
//!   screen reader, an inspector), so the file appears after the first such
//!   request. This is what the shell gives the adapter, before the adapter
//!   or any reader filters it.
//! - `--dump-platform-a11y PATH` (macOS, `ax-dump` feature): every half
//!   second, walk the window's own NSAccessibility elements from inside the
//!   process and write them to PATH, one per line. This is what the
//!   platform adapter exposes to any assistive technology, read without
//!   one. The first walk is itself the request that starts the adapter.
//! - `--activate-a11y` (macOS, `ax-dump` feature): walk the window's
//!   accessibility elements once, half a second in, which starts the
//!   platform adapter and GPUI's accessibility tree, as a screen reader
//!   would; for measuring what that costs while streaming.
//! - `--stream-markdown`: stream markdown (lists, task items, quotes, code)
//!   instead of plain words.
//! - `--open-panel sessions|context`: open that panel at start, as its
//!   toggle would (a modal dialog where the window is too narrow to dock
//!   it), so a window's accessibility tree can be checked with no input.
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

const USAGE: &str = "usage: formless-desktop [--connect LOCAL_SOCKET_OR_PIPE | --start-daemon] [--sessions N] [--blocks N] [--huge-block-mb N] \
                     [--text-scale P] [--preferences PATH | --no-preferences] [--window WxH] [--keep-window] \
                     [--background] [--lifetime S] [--dump-a11y PATH] \
                     [--dump-platform-a11y PATH] \
                     [--smoke] [--stream-seconds S [--stream-rate R]]";

struct Options {
    start_daemon: bool,
    connect: Option<std::path::PathBuf>,
    sessions: usize,
    blocks: usize,
    huge_block_mb: usize,
    text_scale: Option<usize>,
    preferences: Option<std::path::PathBuf>,
    no_preferences: bool,
    window: (f32, f32),
    keep_window: bool,
    open_panel: Option<bool>,
    background: bool,
    lifetime_seconds: usize,
    dump_a11y: Option<String>,
    dump_platform_a11y: Option<String>,
    activate_a11y: bool,
    stream_markdown: bool,
    smoke: bool,
    stream_seconds: usize,
    stream_rate: usize,
}

impl Options {
    fn preference_path(
        &self,
        default: impl FnOnce() -> Option<std::path::PathBuf>,
    ) -> Option<std::path::PathBuf> {
        if self.no_preferences {
            return None;
        }
        self.preferences
            .clone()
            .or_else(|| self.connect.as_ref().and_then(|_| default()))
    }
}

fn parse_options() -> Result<Options, String> {
    parse_options_from(std::env::args().skip(1))
}

fn parse_options_from(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        start_daemon: false,
        connect: None,
        sessions: 10_000,
        blocks: 10_000,
        huge_block_mb: 0,
        text_scale: None,
        preferences: None,
        no_preferences: false,
        window: (1440., 900.),
        keep_window: false,
        open_panel: None,
        background: false,
        lifetime_seconds: 0,
        dump_a11y: None,
        dump_platform_a11y: None,
        smoke: false,
        stream_seconds: 0,
        stream_rate: 100,
        activate_a11y: false,
        stream_markdown: false,
    };
    while let Some(arg) = args.next() {
        let mut count = |name: &str| -> Result<usize, String> {
            let value = args.next().ok_or(format!("{name} needs a number"))?;
            value
                .parse()
                .map_err(|_| format!("{name}: {value:?} is not a number"))
        };
        match arg.as_str() {
            "--start-daemon" => options.start_daemon = true,
            "--connect" => {
                options.connect = Some(
                    args.next()
                        .ok_or("--connect needs a local socket or named pipe path")?
                        .into(),
                )
            }
            "--sessions" => options.sessions = count("--sessions")?,
            "--blocks" => options.blocks = count("--blocks")?,
            "--huge-block-mb" => options.huge_block_mb = count("--huge-block-mb")?,
            "--text-scale" => options.text_scale = Some(count("--text-scale")?),
            "--preferences" => {
                let path = args
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or("--preferences needs a nonempty file path")?;
                options.preferences = Some(path.into());
            }
            "--no-preferences" => options.no_preferences = true,
            "--window" => {
                let value = args.next().ok_or("--window needs WIDTHxHEIGHT")?;
                options.window = value
                    .split_once('x')
                    .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
                    .ok_or(format!("--window: {value:?} is not WIDTHxHEIGHT"))?;
            }
            "--keep-window" => options.keep_window = true,
            "--open-panel" => {
                options.open_panel = match args.next().as_deref() {
                    Some("sessions") => Some(true),
                    Some("context") => Some(false),
                    other => {
                        return Err(format!(
                            "--open-panel: {other:?} is not sessions or context"
                        ))
                    }
                };
            }
            "--background" => options.background = true,
            "--lifetime" => options.lifetime_seconds = count("--lifetime")?,
            "--dump-platform-a11y" => {
                options.dump_platform_a11y = Some(
                    args.next()
                        .ok_or("--dump-platform-a11y needs a file path")?,
                );
            }
            "--dump-a11y" => {
                options.dump_a11y = Some(args.next().ok_or("--dump-a11y needs a file path")?);
            }
            "--smoke" => options.smoke = true,
            "--stream-seconds" => options.stream_seconds = count("--stream-seconds")?,
            "--stream-rate" => options.stream_rate = count("--stream-rate")?,
            "--activate-a11y" => options.activate_a11y = true,
            "--stream-markdown" => options.stream_markdown = true,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    if options.connect.is_some() && (options.stream_seconds > 0 || options.huge_block_mb > 0) {
        return Err(
            "--connect cannot be combined with synthetic streaming or huge-block fixtures".into(),
        );
    }
    if options.no_preferences && options.preferences.is_some() {
        return Err("--preferences and --no-preferences cannot be combined".into());
    }
    Ok(options)
}

/// The window's NSAccessibility elements, read from inside the process.
#[cfg(all(target_os = "macos", feature = "ax-dump"))]
// The `objc` macros test a `cfg` this crate does not declare.
#[allow(unexpected_cfgs)]
mod platform_a11y {
    use std::ffi::CStr;
    use std::fmt::Write as _;
    use std::os::raw::c_char;

    use gpui_kit::Window;
    use objc::runtime::{Object, Sel, BOOL, NO};
    use objc::{msg_send, sel, sel_impl, Message as _};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// More than any real tree here; stops a cycle from running away.
    const MAX_ELEMENTS: usize = 5_000;

    unsafe fn responds(object: *mut Object, selector: Sel) -> bool {
        let responds: BOOL = msg_send![object, respondsToSelector: selector];
        responds != NO
    }

    /// An attribute as text: a string as it is, anything else by its
    /// description. `None` if the element has no such attribute or it is nil.
    unsafe fn text(object: *mut Object, selector: Sel) -> Option<String> {
        if !responds(object, selector) {
            return None;
        }
        let value: *mut Object = (*object).send_message(selector, ()).ok()?;
        if value.is_null() {
            return None;
        }
        let string: *mut Object = if responds(value, sel!(UTF8String)) {
            value
        } else {
            msg_send![value, description]
        };
        let utf8: *const c_char = msg_send![string, UTF8String];
        (!utf8.is_null()).then(|| CStr::from_ptr(utf8).to_string_lossy().into_owned())
    }

    /// Whether the element says the selector applies to it. The platform
    /// adapter's elements all implement every selector and say which ones
    /// are meant through `isAccessibilitySelectorAllowed:`; an attribute
    /// read without asking that would be an answer nobody is given.
    unsafe fn allowed(object: *mut Object, selector: Sel) -> bool {
        if !responds(object, selector) {
            return false;
        }
        if !responds(object, sel!(isAccessibilitySelectorAllowed:)) {
            return true;
        }
        let allowed: BOOL = msg_send![object, isAccessibilitySelectorAllowed: selector];
        allowed != NO
    }

    /// How many elements an array-valued attribute holds, if it applies.
    unsafe fn count(object: *mut Object, selector: Sel) -> Option<usize> {
        if !allowed(object, selector) {
            return None;
        }
        let array: *mut Object = (*object).send_message(selector, ()).ok()?;
        if array.is_null() {
            return None;
        }
        Some(msg_send![array, count])
    }

    unsafe fn flag(object: *mut Object, selector: Sel) -> Option<bool> {
        if !responds(object, selector) {
            return None;
        }
        let value: BOOL = (*object).send_message(selector, ()).ok()?;
        Some(value != NO)
    }

    unsafe fn walk_element(element: *mut Object, depth: usize, out: &mut String, left: &mut usize) {
        if element.is_null() || *left == 0 {
            return;
        }
        *left -= 1;
        let _ = write!(out, "{}", "  ".repeat(depth));
        let _ = write!(
            out,
            "{}",
            text(element, sel!(accessibilityRole)).unwrap_or_else(|| "(no role)".into())
        );
        for (name, selector) in [
            ("subrole", sel!(accessibilitySubrole)),
            ("label", sel!(accessibilityLabel)),
            ("title", sel!(accessibilityTitle)),
            ("value", sel!(accessibilityValue)),
            ("placeholder", sel!(accessibilityPlaceholderValue)),
            ("help", sel!(accessibilityHelp)),
        ] {
            if let Some(value) = text(element, selector).filter(|value| !value.is_empty()) {
                let _ = write!(out, " {name}={value:?}");
            }
        }
        for (name, selector) in [
            ("element", sel!(isAccessibilityElement)),
            ("enabled", sel!(isAccessibilityEnabled)),
            ("focused", sel!(isAccessibilityFocused)),
        ] {
            if let Some(value) = flag(element, selector) {
                let _ = write!(out, " {name}={value}");
            }
        }
        // Only where the element says the attribute applies to it.
        if let Some(description) = text(element, sel!(accessibilityRoleDescription)) {
            let _ = write!(out, " roledescription={description:?}");
        }
        for (name, selector) in [
            ("selected", sel!(isAccessibilitySelected)),
            ("modal", sel!(isAccessibilityModal)),
        ] {
            if allowed(element, selector) {
                if let Some(value) = flag(element, selector) {
                    let _ = write!(out, " {name}={value}");
                }
            }
        }
        for (name, selector) in [
            ("rows", sel!(accessibilityRows)),
            ("selected_rows", sel!(accessibilitySelectedRows)),
            ("selected_children", sel!(accessibilitySelectedChildren)),
        ] {
            if let Some(number) = count(element, selector) {
                let _ = write!(out, " {name}={number}");
            }
        }
        let can: Vec<&str> = [
            ("press", sel!(accessibilityPerformPress)),
            ("pick", sel!(accessibilityPerformPick)),
            ("set_selected", sel!(setAccessibilitySelected:)),
            ("set_focused", sel!(setAccessibilityFocused:)),
            ("set_value", sel!(setAccessibilityValue:)),
        ]
        .into_iter()
        .filter(|(_, selector)| {
            responds(element, sel!(isAccessibilitySelectorAllowed:)) && allowed(element, *selector)
        })
        .map(|(name, _)| name)
        .collect();
        if !can.is_empty() {
            let _ = write!(out, " can=[{}]", can.join(","));
        }
        out.push('\n');
        if !responds(element, sel!(accessibilityChildren)) {
            return;
        }
        let children: *mut Object = msg_send![element, accessibilityChildren];
        if children.is_null() {
            return;
        }
        let count: usize = msg_send![children, count];
        for index in 0..count {
            let child: *mut Object = msg_send![children, objectAtIndex: index];
            walk_element(child, depth + 1, out, left);
        }
    }

    /// One line for each element under the window's content view, indented
    /// by depth: role, then the attributes it has. `element=false` marks an
    /// element the platform tells assistive technology to skip.
    pub fn walk(window: &Window) -> String {
        // The trait's method, not the window's own of the same name.
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return "no window handle
"
            .into();
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return "not an AppKit window
"
            .into();
        };
        let view = handle.ns_view.as_ptr().cast::<Object>();
        let mut out = String::new();
        let mut left = MAX_ELEMENTS;
        // Safety: this runs on the main thread, inside a window update, with
        // the window's own view. Every message is sent only to an object
        // that says it responds to it.
        unsafe {
            // The platform adapter attaches to the window's content view,
            // which need not be the view GPUI draws into.
            let window: *mut Object = msg_send![view, window];
            let content: *mut Object = if window.is_null() {
                view
            } else {
                msg_send![window, contentView]
            };
            if content != view {
                out.push_str("(walking the window's content view, not the drawing view)\n");
            }
            walk_element(content, 0, &mut out, &mut left);
        }
        if left == 0 {
            out.push_str(
                "(stopped at the element limit)
",
            );
        }
        out
    }
}

#[cfg(not(all(target_os = "macos", feature = "ax-dump")))]
mod platform_a11y {
    pub fn walk(_window: &gpui_kit::Window) -> String {
        "the platform accessibility walk needs macOS and the ax-dump feature
"
        .into()
    }
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
fn run_stream(
    window: &Window,
    shell: Entity<Shell>,
    seconds: usize,
    rate: usize,
    markdown: bool,
    cx: &mut App,
) {
    let handle = window.window_handle();
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
                            let delta = if markdown {
                                fixture::stream_markdown_delta(index)
                            } else {
                                fixture::stream_delta(index)
                            };
                            shell.push_stream_delta(delta, cx);
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
            // Whether GPUI was building an accessibility tree, and what
            // making the rows' plain text cost meanwhile.
            let a11y_active = cx
                .update_window(handle, |_, window, _| window.is_a11y_active())
                .unwrap_or(false);
            let (builds, nanos, bytes) = formless_desktop::timeline::accessible_text_stats();
            println!(
                "a11y: active={a11y_active} accessible_text_builds={builds} \
                 accessible_text_ms={:.1} accessible_text_bytes={bytes} markdown_stream={markdown}",
                nanos as f64 / 1e6
            );
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

struct StartupErrorWindow;

impl gpui_kit::Render for StartupErrorWindow {
    fn render(
        &mut self,
        _: &mut Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        gpui_kit::div()
    }
}

fn main() {
    let started = Instant::now();
    let mut options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("formless-desktop: {message}");
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };

    let args: Vec<_> = std::env::args().skip(1).collect();
    let executable = std::env::current_exe().unwrap_or_default();
    let mut startup_error = None;
    if formless_desktop::bootstrap::should_start(
        &executable,
        &args,
        options.start_daemon,
        options.connect.is_some(),
    ) {
        match formless_desktop::bootstrap::start() {
            Ok(endpoint) => options.connect = Some(endpoint),
            Err(error) => {
                eprintln!("formless-desktop: daemon startup failed: {error}");
                startup_error = Some(error);
            }
        }
    }

    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        shell::bind_keys(cx);

        if let Some(error) = startup_error {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_window_title("Formless");
                let view = cx.new(|_| StartupErrorWindow);
                let answer = window.prompt(
                    gpui_kit::PromptLevel::Critical,
                    "Formless couldn't start its daemon",
                    Some(&error),
                    &["Quit"],
                    cx,
                );
                cx.spawn(async move |cx| {
                    let _ = answer.await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
                view
            })
            .expect("failed to open startup error window");
            return;
        }

        let app_ready = started.elapsed();
        let sessions = if options.connect.is_some() {
            Vec::new()
        } else {
            fixture::sessions(options.sessions)
        };
        let running = fixture::running(&sessions);
        let mut blocks = if options.connect.is_some() {
            Vec::new()
        } else {
            fixture::timeline(options.blocks)
        };
        if options.huge_block_mb > 0 && options.connect.is_none() {
            blocks.push(fixture::huge_block(
                blocks.len() as u64 + 1,
                options.huge_block_mb * 1024 * 1024,
            ));
        }
        let fixture_built = started.elapsed() - app_ready;

        // Fixture/diagnostic invocations never load personal settings unless
        // a path was explicitly requested. Loading does not create files.
        let preference_store = options
            .preference_path(formless_desktop::preferences::default_path)
            .map(formless_desktop::preferences::Store::open);
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
            #[cfg(target_os = "macos")]
            {
                gpui_kit::component::Theme::sync_system_appearance(Some(window), cx);
                formless_desktop::theme::apply(cx);
            }
            window.set_window_title(if options.connect.is_some() {
                "Formless"
            } else {
                "Formless (Phase 0 spike)"
            });
            let shell = cx.new(|cx| shell::Shell::new(&sessions, &running, blocks, window, cx));
            shell.update(cx, |shell, cx| {
                #[cfg(target_os = "macos")]
                shell.install_native_composer_escape(window, cx);
                if let Some(store) = preference_store {
                    shell.attach_preferences(store, window, cx);
                }
                let scale = options.text_scale.map_or(shell.text_scale(), |percent| {
                    TextScale::from_percent(percent.min(1_000) as u16)
                });
                if let Some(endpoint) = options.connect.clone() {
                    shell.connect_daemon(formless_desktop::client::Endpoint(endpoint), cx);
                }
                if options.keep_window {
                    shell.set_text_scale(scale, window, cx)
                } else {
                    shell.set_text_scale_growing_window(scale, window, cx)
                }
                if let Some(sessions) = options.open_panel {
                    shell.open_panel(sessions, window, cx);
                }
            });
            #[cfg(target_os = "macos")]
            {
                let shell = shell.downgrade();
                window
                    .observe_window_appearance(move |window, cx| {
                        let _ = shell.update(cx, |shell, cx| {
                            shell.sync_system_appearance(window, cx);
                        });
                    })
                    .detach();
            }
            shell
        })
        .expect("failed to open the shell window");
        if options.connect.is_some() {
            let handle = window;
            let shell = shell.downgrade();
            cx.spawn(async move |cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let result = cx.update(|cx| {
                    cx.update_window(handle, |_, window, cx| {
                        shell.update(cx, |shell, cx| shell.poll_client(window, cx))
                    })
                });
                if !matches!(result, Ok(Ok(()))) {
                    break;
                }
            })
            .detach();
        }
        if !options.background {
            cx.activate(true);
        }
        if let Some(path) = options.dump_a11y.clone() {
            let handle = window;
            cx.spawn(async move |cx| {
                let mut was_active = false;
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(500))
                        .await;
                    let dumped = cx.update(|cx| {
                        cx.update_window(handle, |_, window, _| {
                            (window.is_a11y_active(), window.debug_a11y_tree_json())
                        })
                    });
                    let Ok((active, tree)) = dumped else { break };
                    if active != was_active {
                        println!("a11y: active={active}");
                        was_active = active;
                    }
                    if let Some(tree) = tree {
                        if let Err(error) = std::fs::write(&path, tree) {
                            println!("a11y: cannot write {path}: {error}");
                            break;
                        }
                    }
                }
            })
            .detach();
        }
        if options.activate_a11y {
            // One walk of the window's accessibility elements, half a second
            // in, then nothing: asking is what starts the platform adapter
            // and so GPUI's accessibility tree, without a walk every half
            // second adding its own cost to what is measured.
            let handle = window;
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let walked = cx.update(|cx| {
                    cx.update_window(handle, |_, window, _| platform_a11y::walk(window).len())
                });
                println!(
                    "a11y: activated by one walk ({} bytes)",
                    walked.unwrap_or(0)
                );
            })
            .detach();
        }
        if let Some(path) = options.dump_platform_a11y.clone() {
            let handle = window;
            cx.spawn(async move |cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let walked = cx.update(|cx| {
                    cx.update_window(handle, |_, window, _| platform_a11y::walk(window))
                });
                let Ok(tree) = walked else { break };
                if let Err(error) = std::fs::write(&path, tree) {
                    println!("a11y: cannot write {path}: {error}");
                    break;
                }
            })
            .detach();
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
                            shell.read(cx).text_scale().percent(),
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
                                options.stream_markdown,
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

#[cfg(test)]
mod preference_option_tests {
    use super::*;
    #[test]
    fn daemon_option_parsing() {
        assert!(!options(&[]).unwrap().start_daemon);
        assert!(options(&["--start-daemon"]).unwrap().start_daemon);
        let both = options(&["--start-daemon", "--connect", "socket"]).unwrap();
        assert_eq!(both.connect, Some("socket".into()));
        assert!(options(&["--start-daemon", "--unknown"]).is_err());
    }
    fn options(args: &[&str]) -> Result<Options, String> {
        parse_options_from(args.iter().map(|s| s.to_string()))
    }
    #[test]
    fn fixtures_do_not_resolve_personal_preferences_and_explicit_paths_win() {
        let default = options(&[]).unwrap();
        assert!(default
            .preference_path(|| panic!("fixture must not resolve personal preferences"))
            .is_none());
        assert!(default.text_scale.is_none());
        let explicit =
            options(&["--preferences", "private/view.json", "--text-scale", "175"]).unwrap();
        assert_eq!(
            explicit.preference_path(|| panic!("explicit path must not resolve default")),
            Some("private/view.json".into())
        );
        assert_eq!(explicit.text_scale, Some(175));
    }
    #[test]
    fn connected_mode_resolves_default_unless_disabled_and_rejects_conflicting_flags() {
        let live = options(&["--connect", "private.sock"]).unwrap();
        assert_eq!(
            live.preference_path(|| Some("owned/preferences.json".into())),
            Some("owned/preferences.json".into())
        );
        let disabled = options(&["--connect", "private.sock", "--no-preferences"]).unwrap();
        assert!(disabled
            .preference_path(|| panic!("disabled path must not be resolved"))
            .is_none());
        assert!(options(&["--preferences", ""]).is_err());
        assert!(options(&["--preferences"]).is_err());
        assert!(options(&["--preferences", "view.json", "--no-preferences"]).is_err());
    }
}
