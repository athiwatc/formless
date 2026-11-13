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
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use formless_desktop::fixture;
    use formless_desktop::model::{Fit, TextScale};
    use formless_desktop::shell::{self, PageSection, Shell};
    use gpui_kit::assets::Assets;
    use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        point, px, size, AnyWindowHandle, App, AppContext as _, Bounds, Entity, HeadlessAppContext,
        Pixels, ScrollDelta, Size, Window, WindowBounds, WindowOptions,
    };

    /// One state to draw: window size, theme, text size, a control to click
    /// first, and how the shell should be fitted into that window.
    struct State {
        name: &'static str,
        width: f32,
        height: f32,
        mode: ThemeMode,
        percent: u16,
        click: Option<&'static str>,
        fit: Fit,
    }

    const fn state(
        name: &'static str,
        width: f32,
        height: f32,
        mode: ThemeMode,
        percent: u16,
        click: Option<&'static str>,
        fit: Fit,
    ) -> State {
        State {
            name,
            width,
            height,
            mode,
            percent,
            click,
            fit,
        }
    }

    const LIGHT: ThemeMode = ThemeMode::Light;
    const DARK: ThemeMode = ThemeMode::Dark;
    const WHOLE: Fit = Fit::Whole;
    const REFLOWED: Fit = Fit::Reflowed;

    const STATES: [State; 21] = [
        state("wide-light", 1440., 900., LIGHT, 100, None, WHOLE),
        state("wide-dark", 1440., 900., DARK, 100, None, WHOLE),
        state("medium-light", 1100., 900., LIGHT, 100, None, WHOLE),
        state("narrow-dark", 800., 900., DARK, 100, None, WHOLE),
        state(
            "medium-inspector-overlay-light",
            1100.,
            900.,
            LIGHT,
            100,
            Some("toggle-inspector"),
            WHOLE,
        ),
        state(
            "narrow-sidebar-overlay-dark",
            800.,
            900.,
            DARK,
            100,
            Some("toggle-sidebar"),
            WHOLE,
        ),
        state(
            "wide-tools-scaffold-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("destination-Tools"),
            WHOLE,
        ),
        state("wide-light-200pct", 1440., 900., LIGHT, 200, None, WHOLE),
        state(
            "wide-sidebar-overlay-light-200pct",
            1440.,
            900.,
            LIGHT,
            200,
            Some("toggle-sidebar"),
            WHOLE,
        ),
        // The binary's minimum window, and the smallest window that shows
        // everything at once at 200%.
        state("minimum-light-100pct", 480., 360., LIGHT, 100, None, WHOLE),
        state("fits-200pct-light", 960., 720., LIGHT, 200, None, WHOLE),
        // Windows smaller than the text size needs. The text stays the
        // size asked for and the shell is reflowed.
        state("minimum-dark-200pct", 480., 360., DARK, 200, None, REFLOWED),
        state(
            "minimum-light-200pct",
            480.,
            360.,
            LIGHT,
            200,
            None,
            REFLOWED,
        ),
        state(
            "minimum-light-200pct-sessions",
            480.,
            360.,
            LIGHT,
            200,
            Some("toggle-sidebar"),
            REFLOWED,
        ),
        state(
            "minimum-dark-200pct-context",
            480.,
            360.,
            DARK,
            200,
            Some("toggle-inspector"),
            REFLOWED,
        ),
        // The command palette, opened from its header button.
        state(
            "wide-palette-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("show-commands"),
            WHOLE,
        ),
        state(
            "minimum-dark-200pct-palette",
            480.,
            360.,
            DARK,
            200,
            Some("show-commands"),
            REFLOWED,
        ),
        state(
            "minimum-light-150pct",
            480.,
            360.,
            LIGHT,
            150,
            None,
            REFLOWED,
        ),
        state(
            "just-short-of-200pct-dark",
            959.,
            719.,
            DARK,
            200,
            None,
            REFLOWED,
        ),
        // Narrow but tall, and wide but short: either is enough to reflow.
        state(
            "narrow-tall-200pct-light",
            480.,
            900.,
            LIGHT,
            200,
            None,
            REFLOWED,
        ),
        state(
            "wide-short-200pct-dark",
            1440.,
            500.,
            DARK,
            200,
            None,
            REFLOWED,
        ),
    ];

    /// Controls that must be whole and inside the window, all at once, where
    /// the shell is whole.
    const ALWAYS_REACHABLE: [&str; 7] = [
        "toggle-sidebar",
        "toggle-inspector",
        "toggle-theme",
        "show-commands",
        "composer-stop",
        "composer-primary",
        "status",
    ];

    /// The parts of a reflowed page, top to bottom.
    const PAGE_PARTS: [&str; 4] = ["header", "region-conversation", "region-composer", "status"];

    /// What must be whole and inside the window once focus is in each part
    /// of a reflowed page.
    const SECTIONS: [(PageSection, &str, &[&str]); 4] = [
        (
            PageSection::Header,
            "header",
            &[
                "header",
                "toggle-sidebar",
                "toggle-inspector",
                "toggle-theme",
                "show-commands",
            ],
        ),
        (
            PageSection::Conversation,
            "conversation",
            &["region-conversation"],
        ),
        (
            PageSection::Composer,
            "composer",
            &["region-composer", "composer-stop", "composer-primary"],
        ),
        (PageSection::Status, "status", &["status"]),
    ];

    /// Sizes of controls where the shell is whole, by text size, to hold the
    /// reflowed shell to: the same controls must be exactly as large there.
    type ControlSizes = HashMap<(u16, &'static str), Size<Pixels>>;
    const MEASURED: [&str; 3] = ["toggle-sidebar", "composer-primary", "composer-stop"];

    fn inside(window: &Window, id: &'static str, viewport: Size<Pixels>) -> bool {
        let bounds = window.find(id).bounds();
        bounds.left() >= px(0.)
            && bounds.right() <= viewport.width
            && bounds.top() >= px(0.)
            && bounds.bottom() <= viewport.height
    }

    /// Checks for a window that holds everything at once.
    fn check_whole(name: &str, viewport: Size<Pixels>, window: &Window) {
        for id in ALWAYS_REACHABLE {
            assert!(
                inside(window, id, viewport),
                "{name}: {id} is not inside the window"
            );
        }
        assert!(
            window.find("region-composer").bounds().bottom()
                <= window.find("status").bounds().top(),
            "{name}: the status line overlaps the composer"
        );
        assert!(
            window.try_find("shell-page").is_none(),
            "{name}: a window that holds everything must not scroll as a page"
        );
    }

    /// Checks for a reflowed window, with real fonts: nothing is cut off at
    /// the side, the parts stack without overlapping, and the page's end can
    /// be scrolled into the window.
    fn check_reflowed(
        name: &str,
        viewport: Size<Pixels>,
        shell: &Entity<Shell>,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Nothing is wider than the window or starts outside it, so the
        // page never needs to scroll sideways.
        let ids = PAGE_PARTS.iter().chain(MEASURED.iter()).chain(
            [
                "toggle-inspector",
                "toggle-theme",
                "show-commands",
                "run-status",
            ]
            .iter(),
        );
        for id in ids {
            let bounds = window.find(*id).bounds();
            assert!(
                bounds.left() >= px(0.) && bounds.right() <= viewport.width,
                "{name}: {id} runs {:?}..{:?}, outside a window {:?} wide",
                bounds.left(),
                bounds.right(),
                viewport.width
            );
        }
        // The parts stack in order and do not overlap.
        for pair in PAGE_PARTS.windows(2) {
            let (above, below) = (window.find(pair[0]).bounds(), window.find(pair[1]).bounds());
            assert!(
                above.bottom() <= below.top(),
                "{name}: {} ({:?}) overlaps {} ({:?})",
                pair[0],
                above.bottom(),
                pair[1],
                below.top()
            );
            assert!(
                above.size.height > px(0.),
                "{name}: {} has no height",
                pair[0]
            );
        }
        // The header's controls do not overlap each other either.
        let controls: Vec<Bounds<Pixels>> = [
            "toggle-sidebar",
            "toggle-inspector",
            "toggle-theme",
            "show-commands",
        ]
        .into_iter()
        .map(|id| window.find(id).bounds())
        .collect();
        for (index, a) in controls.iter().enumerate() {
            for b in &controls[index + 1..] {
                assert!(
                    !a.intersects(b),
                    "{name}: header controls overlap: {a:?} {b:?}"
                );
            }
        }
        // The page is taller than the window, or exactly fills it.
        let page = shell.read(cx).page_metrics();
        assert_eq!(page.sections.len(), PAGE_PARTS.len(), "{name}: page parts");
        assert_eq!(
            page.viewport.size, viewport,
            "{name}: the page is the window"
        );
        // Scrolled to its end, the page's last part ends inside the window:
        // the status line is reachable.
        let (_, last) = page.sections.last().expect("the page has parts");
        assert!(
            last.bottom() - page.scroll_max <= viewport.height,
            "{name}: the status line cannot be scrolled into the window"
        );
    }

    fn draw_state(
        State {
            name,
            width,
            height,
            mode,
            percent,
            click,
            fit,
        }: State,
        keep: Option<&str>,
        sizes: &mut ControlSizes,
    ) -> usize {
        let viewport = size(px(width), px(height));
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
                        size: viewport,
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                };
                gpui_kit::open_window(options, cx, |window, cx| {
                    let shell = cx.new(|cx| Shell::new(&sessions, &running, blocks, window, cx));
                    shell.update(cx, |shell, cx| {
                        shell.set_text_scale(TextScale::from_percent(percent), window, cx)
                    });
                    shell
                })
            })
            .expect("open offscreen window");
        cx.run_until_parked();
        let any: AnyWindowHandle = handle;
        let mut captures = 0;
        let mut capture = |cx: &mut HeadlessAppContext, suffix: &str| {
            let image = cx
                .capture_screenshot(any)
                .expect("offscreen rendering must be available");
            let (image_width, image_height) = image.dimensions();
            let colours: HashSet<[u8; 4]> = image.pixels().map(|pixel| pixel.0).collect();
            assert!(
                colours.len() > 64,
                "{name}{suffix}: only {} distinct colours, the shell did not render",
                colours.len()
            );
            if let Some(dir) = keep {
                image
                    .save(format!("{dir}/shell-{name}{suffix}.png"))
                    .expect("write capture");
            }
            println!(
                "render: {name}{suffix} {image_width}x{image_height} device px, {} distinct colours",
                colours.len()
            );
            captures += 1;
        };

        // With real fonts, before any overlay is opened over the shell.
        cx.update_window(any, |_, window, cx| {
            window.render_frame(cx);
            // The text is the size that was asked for, whatever the window.
            let factor = f32::from(percent) / 100.;
            assert_eq!(shell.read(cx).text_scale().percent(), percent, "{name}");
            assert_eq!(cx.theme().font_size, px(16. * factor), "{name}: font size");
            assert_eq!(window.rem_size(), px(16. * factor), "{name}: rem size");
            assert_eq!(shell.read(cx).fit(window), fit, "{name}: fit");
            for id in MEASURED {
                let measured = window.find(id).bounds().size;
                match fit {
                    Fit::Whole => {
                        sizes.insert((percent, id), measured);
                    }
                    // A control is exactly as large reflowed as it is where
                    // the shell is whole at the same text size. Only widths
                    // can differ, where a control is given a row to itself.
                    Fit::Reflowed => {
                        if let Some(whole) = sizes.get(&(percent, id)) {
                            assert_eq!(
                                measured.height, whole.height,
                                "{name}: {id} is not the height it is at {percent}%"
                            );
                            assert!(
                                measured.width >= whole.width,
                                "{name}: {id} is narrower than it is at {percent}%"
                            );
                        }
                    }
                }
            }
            match fit {
                Fit::Whole => check_whole(name, viewport, window),
                Fit::Reflowed => check_reflowed(name, viewport, &shell, window, cx),
            }
        })
        .expect("offscreen window is open");

        if fit == Fit::Reflowed {
            // Focus brings each part wholly into the window.
            for (section, label, ids) in SECTIONS {
                cx.update_window(any, |_, window, cx| {
                    shell.update(cx, |shell, cx| shell.focus_section(section, window, cx));
                    // The frame the shell asks for is the one that scrolls.
                    window.render_frame(cx);
                    assert!(
                        window.simulate_next_frame(cx) > 0,
                        "{name}: no second frame"
                    );
                })
                .expect("offscreen window is open");
                cx.run_until_parked();
                cx.update_window(any, |_, window, cx| {
                    window.render_frame(cx);
                    assert_eq!(
                        shell.read(cx).focused_section(window, cx),
                        Some(section),
                        "{name}: focus"
                    );
                    for id in ids {
                        assert!(
                            inside(window, id, viewport),
                            "{name}: with focus in the {label}, {id} is at {:?}, not inside the window",
                            window.find(*id).bounds()
                        );
                    }
                })
                .expect("offscreen window is open");
                if click.is_none() {
                    capture(&mut cx, &format!("-{label}"));
                }
            }
            // The wheel does it too: from the top of the page, turned over
            // the header, it reaches the end, where the status line is.
            cx.update_window(any, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_section(PageSection::Header, window, cx)
                });
                window.render_frame(cx);
                assert!(inside(window, "toggle-sidebar", viewport), "{name}");
                for _ in 0..40 {
                    // Over a part that does not scroll by itself where one
                    // is in view, and over the timeline where it fills the
                    // window: at its newest block it has nothing further to
                    // scroll to, and the wheel has to go on to the page.
                    let over = ["header", "run-status", "region-composer", "status"]
                        .into_iter()
                        .find(|id| {
                            // The wheel is turned over the part's middle.
                            let middle = window.find(*id).bounds().center().y;
                            middle > px(0.) && middle < viewport.height
                        })
                        .unwrap_or("region-conversation");
                    window.scroll(over, ScrollDelta::Pixels(point(px(0.), px(-120.))), cx);
                    window.render_frame(cx);
                }
                let page = shell.read(cx).page_metrics();
                assert_eq!(
                    page.scrolled, page.scroll_max,
                    "{name}: the wheel did not reach the end of the page"
                );
                assert!(
                    page.is_inside(PageSection::Status) && inside(window, "status", viewport),
                    "{name}: at the end of the page the status line is at {:?}",
                    window.find("status").bounds()
                );
            })
            .expect("offscreen window is open");
            // Back to the header, where the controls to click are.
            cx.update_window(any, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_section(PageSection::Header, window, cx)
                });
                window.render_frame(cx);
            })
            .expect("offscreen window is open");
            cx.run_until_parked();
        }

        if let Some(control) = click {
            cx.update_window(any, |_, window, cx| {
                window.render_frame(cx);
                window.click(control, cx);
            })
            .expect("offscreen window is open");
            cx.run_until_parked();
            if fit == Fit::Reflowed {
                // The overlay panel is inside the window, edge to edge at
                // most, whatever the page under it was scrolled to.
                cx.update_window(any, |_, window, cx| {
                    window.render_frame(cx);
                    let panel = match control {
                        "toggle-sidebar" => "region-sidebar",
                        "show-commands" => "palette",
                        _ => "region-inspector",
                    };
                    assert!(
                        inside(window, panel, viewport),
                        "{name}: {panel} is at {:?}, not inside the window",
                        window.find(panel).bounds()
                    );
                })
                .expect("offscreen window is open");
            }
            capture(&mut cx, "");
        } else if fit == Fit::Whole {
            capture(&mut cx, "");
        }
        captures
    }

    pub fn run() {
        let keep = std::env::var("FORMLESS_DESKTOP_RENDER_DIR").ok();
        let mut sizes = ControlSizes::new();
        let mut captures = 0;
        let states = STATES.len();
        for state in STATES {
            captures += draw_state(state, keep.as_deref(), &mut sizes);
        }
        println!("render: ok ({states} states, {captures} captures)");
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
