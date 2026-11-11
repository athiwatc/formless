//! Offscreen pixel check of the shell with GPUI's real renderer (Metal on
//! macOS, WGPU on Linux). No window is shown and no fake renderer is used.
//!
//! Set `FORMLESS_DESKTOP_RENDER_DIR` to an existing directory to keep the
//! captures as PNG files for a human to look at. Without it the test only
//! asserts that each state rendered something other than a flat fill, which
//! is far short of a visual review.

fn main() {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    render::run();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    println!("render: skipped; GPUI has no headless renderer on this platform");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod render {
    use std::collections::HashSet;
    use std::sync::Arc;

    use formless_desktop::fixture;
    use formless_desktop::model::TextScale;
    use formless_desktop::shell::{self, Shell};
    use gpui_kit::assets::Assets;
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        px, size, AppContext as _, Bounds, HeadlessAppContext, Pixels, Window, WindowBounds,
        WindowOptions,
    };

    /// One state to draw: window size, theme, the text size asked for, a
    /// control to click first, and the text size that should end up in use.
    struct State {
        name: &'static str,
        width: f32,
        height: f32,
        mode: ThemeMode,
        percent: u16,
        click: Option<&'static str>,
        shown_percent: u16,
    }

    const fn state(
        name: &'static str,
        width: f32,
        height: f32,
        mode: ThemeMode,
        percent: u16,
        click: Option<&'static str>,
        shown_percent: u16,
    ) -> State {
        State {
            name,
            width,
            height,
            mode,
            percent,
            click,
            shown_percent,
        }
    }

    const LIGHT: ThemeMode = ThemeMode::Light;
    const DARK: ThemeMode = ThemeMode::Dark;

    const STATES: [State; 14] = [
        state("wide-light", 1440., 900., LIGHT, 100, None, 100),
        state("wide-dark", 1440., 900., DARK, 100, None, 100),
        state("medium-light", 1100., 900., LIGHT, 100, None, 100),
        state("narrow-dark", 800., 900., DARK, 100, None, 100),
        state(
            "medium-inspector-overlay-light",
            1100.,
            900.,
            LIGHT,
            100,
            Some("toggle-inspector"),
            100,
        ),
        state(
            "narrow-sidebar-overlay-dark",
            800.,
            900.,
            DARK,
            100,
            Some("toggle-sidebar"),
            100,
        ),
        state(
            "wide-tools-scaffold-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("destination-Tools"),
            100,
        ),
        state("wide-light-200pct", 1440., 900., LIGHT, 200, None, 200),
        state(
            "wide-sidebar-overlay-light-200pct",
            1440.,
            900.,
            LIGHT,
            200,
            Some("toggle-sidebar"),
            200,
        ),
        // The binary's minimum window. 200% does not fit it; the default
        // size is used and the status line says so.
        state("minimum-light-100pct", 480., 360., LIGHT, 100, None, 100),
        state("minimum-dark-200pct", 480., 360., DARK, 200, None, 100),
        // The smallest window that holds each larger size.
        state("fits-150pct-light", 720., 540., LIGHT, 200, None, 150),
        state("fits-200pct-light", 960., 720., LIGHT, 200, None, 200),
        state(
            "just-short-of-200pct-dark",
            959.,
            719.,
            DARK,
            200,
            None,
            175,
        ),
    ];

    /// Controls that must be whole and inside the window in every state.
    const ALWAYS_REACHABLE: [&str; 6] = [
        "toggle-sidebar",
        "toggle-inspector",
        "toggle-theme",
        "composer-stop",
        "composer-primary",
        "status",
    ];

    pub fn run() {
        let keep = std::env::var("FORMLESS_DESKTOP_RENDER_DIR").ok();
        for State {
            name,
            width,
            height,
            mode,
            percent,
            click,
            shown_percent,
        } in STATES
        {
            let mut cx = HeadlessAppContext::with_platform(
                gpui_kit::platform::current_platform(true).text_system(),
                Arc::new(Assets),
                gpui_kit::platform::current_headless_renderer,
            );
            let (handle, shell) = cx
                .update(|cx| {
                    gpui_kit::init(cx);
                    shell::bind_keys(cx);
                    Theme::change(mode, None, cx);
                    let sessions = fixture::sessions(200);
                    let running = fixture::running(&sessions);
                    let blocks = fixture::timeline(40);
                    let options = WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: Default::default(),
                            size: size(px(width), px(height)),
                        })),
                        focus: false,
                        show: false,
                        ..Default::default()
                    };
                    gpui_kit::open_window(options, cx, |window, cx| {
                        let shell =
                            cx.new(|cx| Shell::new(&sessions, &running, blocks, window, cx));
                        shell.update(cx, |shell, cx| {
                            shell.set_text_scale(TextScale::from_percent(percent), window, cx)
                        });
                        shell
                    })
                })
                .expect("open offscreen window");
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                // With real fonts: the text size in use is the one this
                // window can hold, and at that size every control is whole
                // and inside the window, with the composer above the status
                // line. (Checked before any overlay is opened over them.)
                assert_eq!(
                    shell.read(cx).shown_text_scale().percent(),
                    shown_percent,
                    "{name}: text size in use"
                );
                let inside = |id: &'static str| {
                    let bounds = window.find(id).bounds();
                    bounds.left() >= px(0.)
                        && bounds.right() <= px(width)
                        && bounds.top() >= px(0.)
                        && bounds.bottom() <= px(height)
                };
                for id in ALWAYS_REACHABLE {
                    assert!(inside(id), "{name}: {id} is not inside the window");
                }
                assert!(
                    window.find("region-composer").bounds().bottom()
                        <= window.find("status").bounds().top(),
                    "{name}: the status line overlaps the composer"
                );
                if let Some(control) = click {
                    window.click(control, cx);
                }
            })
            .expect("offscreen window is open");
            cx.run_until_parked();
            let image = cx
                .capture_screenshot(handle)
                .expect("offscreen rendering must be available");
            let (image_width, image_height) = image.dimensions();
            let colours: HashSet<[u8; 4]> = image.pixels().map(|pixel| pixel.0).collect();
            assert!(
                colours.len() > 64,
                "{name}: only {} distinct colours, the shell did not render",
                colours.len()
            );
            if let Some(dir) = &keep {
                image
                    .save(format!("{dir}/shell-{name}.png"))
                    .expect("write capture");
            }
            println!(
                "render: {name} {image_width}x{image_height} device px, {} distinct colours",
                colours.len()
            );
        }
        println!("render: ok ({} states)", STATES.len());
        real_text_measurements();
    }

    /// Where the real text system wraps `text` at `width`, as byte offsets,
    /// and the width of the text laid out on one line.
    fn wraps(window: &mut Window, text: &str, width: Pixels) -> (Vec<usize>, Pixels) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let run = style.to_run(text.len());
        let lines = window
            .text_system()
            .shape_text(
                text.to_string().into(),
                font_size,
                &[run],
                Some(width),
                None,
            )
            .expect("the text shapes");
        assert_eq!(lines.len(), 1, "one paragraph");
        let line = &lines[0];
        let offsets = line
            .wrap_boundaries
            .iter()
            .map(|boundary| {
                line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
            })
            .collect();
        (offsets, line.unwrapped_layout.width)
    }

    /// Measurements that need real fonts. The kit's headless test window
    /// (tests/shell.rs) uses a stand-in text system in which every character
    /// is the same width, so nothing about glyph widths or where real text
    /// wraps can be learned there. This runs on the platform text system.
    fn real_text_measurements() {
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(Assets),
            gpui_kit::platform::current_headless_renderer,
        );
        let (handle, _) = cx
            .update(|cx| {
                gpui_kit::init(cx);
                let sessions = fixture::sessions(4);
                let running = fixture::running(&sessions);
                let options = WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(1440.), px(900.)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                };
                gpui_kit::open_window(options, cx, |window, cx| {
                    cx.new(|cx| Shell::new(&sessions, &running, fixture::timeline(4), window, cx))
                })
            })
            .expect("open offscreen window");
        cx.update_window(handle, |_, window, _| {
            let thai = "ข้อความอ้างอิงภาษาไทยที่ยาวพอจะตัดบรรทัดได้หลายครั้งเมื่อหน้าต่างแคบลงเพื่อทดสอบการตัดคำ";
            let latin = "the quick brown fox jumps over the lazy dog";
            let (_, thai_width) = wraps(window, thai, px(100_000.));
            let (_, latin_width) = wraps(window, latin, px(100_000.));
            println!(
                "render: real text widths at 16 px: {} Thai characters {thai_width:?}, {} Latin \
                 characters {latin_width:?}",
                thai.chars().count(),
                latin.chars().count()
            );
            // A stand-in text system would make every character equally wide.
            let (_, narrow) = wraps(window, "iiiiiiii", px(100_000.));
            let (_, wide) = wraps(window, "mmmmmmmm", px(100_000.));
            assert!(wide > narrow * 1.5, "these are not real font metrics");
            #[cfg(feature = "thai-breaks")]
            thai_breaks(window, thai);
        })
        .expect("offscreen window is open");
    }

    /// With real fonts: does GPUI break Thai inside words, do zero width
    /// space marks at word boundaries fix that, and do the marks take space?
    #[cfg(feature = "thai-breaks")]
    fn thai_breaks(window: &mut Window, sentence: &str) {
        use formless_desktop::thai::{self, ZERO_WIDTH_SPACE};

        let raw = sentence.repeat(6);
        let marked = thai::with_break_opportunities(&raw).into_owned();
        let word_starts: HashSet<usize> = thai::word_boundaries(&raw).into_iter().collect();
        let marks = marked.matches(ZERO_WIDTH_SPACE).count();
        for width in [px(240.), px(300.), px(420.)] {
            let (raw_wraps, _) = wraps(window, &raw, width);
            let (marked_wraps, _) = wraps(window, &marked, width);
            let raw_mid_word = raw_wraps
                .iter()
                .filter(|offset| !word_starts.contains(offset))
                .count();
            // In the marked text a break between words is at a mark.
            let at_mark = |offset: usize| {
                marked[..offset].ends_with(ZERO_WIDTH_SPACE)
                    || marked[offset..].starts_with(ZERO_WIDTH_SPACE)
            };
            let marked_mid_word = marked_wraps
                .iter()
                .filter(|offset| !at_mark(**offset))
                .count();
            println!(
                "render: thai wrap at {width:?}: unmarked {} breaks, {raw_mid_word} inside a word; \
                 marked {} breaks, {marked_mid_word} inside a word",
                raw_wraps.len(),
                marked_wraps.len()
            );
            assert!(raw_wraps.len() >= 4, "the paragraph wraps");
            assert!(raw_mid_word > 0, "unmarked Thai broke inside a word");
            assert_eq!(marked_mid_word, 0, "marked Thai breaks only between words");
        }
        let (_, raw_width) = wraps(window, &raw, px(100_000.));
        let (_, marked_width) = wraps(window, &marked, px(100_000.));
        let (_, latin) = wraps(window, "abab", px(100_000.));
        let (_, latin_marked) = wraps(window, &format!("ab{ZERO_WIDTH_SPACE}ab"), px(100_000.));
        println!(
            "render: zero width space: {marks} marks between Thai words add {:?} to {raw_width:?} \
             ({:?} each); one between Latin letters adds {:?}",
            marked_width - raw_width,
            (marked_width - raw_width) / marks as f32,
            latin_marked - latin
        );
    }
}
