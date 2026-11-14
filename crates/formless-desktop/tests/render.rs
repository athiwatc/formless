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

    use formless_desktop::commands::Command;
    use formless_desktop::fixture;
    use formless_desktop::model::{BlockKind, Destination, Fit, TextScale};
    use formless_desktop::shell::{self, PageSection, Shell};
    use formless_desktop::timeline::TimelineBlock;
    use gpui_kit::assets::Assets;
    use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        point, px, size, AnyWindowHandle, App, AppContext as _, Bounds, ElementId, Entity,
        Focusable as _, HeadlessAppContext, Pixels, ScrollDelta, Size, Window, WindowBounds,
        WindowOptions,
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
        viewer: bool,
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
            viewer: false,
        }
    }

    const fn viewer_state(mut state: State) -> State {
        state.viewer = true;
        state
    }

    const LIGHT: ThemeMode = ThemeMode::Light;
    const DARK: ThemeMode = ThemeMode::Dark;
    const WHOLE: Fit = Fit::Whole;
    const REFLOWED: Fit = Fit::Reflowed;

    const STATES: [State; 41] = [
        state(
            "profile-questions-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("open-profile-questions"),
            WHOLE,
        ),
        state(
            "minimum-profile-questions-dark-200pct",
            480.,
            360.,
            DARK,
            200,
            Some("open-profile-questions"),
            REFLOWED,
        ),
        state(
            "question-form-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("open-profile-questions-form"),
            WHOLE,
        ),
        state(
            "minimum-question-form-dark-200pct",
            480.,
            360.,
            DARK,
            200,
            Some("open-profile-questions-form"),
            REFLOWED,
        ),
        state(
            "daemon-forecast-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("open-run-forecast"),
            WHOLE,
        ),
        state(
            "minimum-daemon-forecast-dark-200pct",
            480.,
            360.,
            DARK,
            200,
            Some("open-run-forecast"),
            REFLOWED,
        ),
        state(
            "daemon-recovery-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("open-run-recovery"),
            WHOLE,
        ),
        state(
            "minimum-daemon-recovery-dark-200pct",
            480.,
            360.,
            DARK,
            200,
            Some("open-run-recovery"),
            REFLOWED,
        ),
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
        viewer_state(state(
            "wide-whole-output-light",
            1440.,
            900.,
            LIGHT,
            100,
            None,
            WHOLE,
        )),
        viewer_state(state(
            "minimum-whole-output-dark-200pct",
            480.,
            360.,
            DARK,
            200,
            None,
            REFLOWED,
        )),
        state(
            "wide-sessions-directory-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("open-directory"),
            WHOLE,
        ),
        state(
            "minimum-sessions-directory-dark-200pct",
            480.,
            360.,
            DARK,
            200,
            Some("open-directory"),
            REFLOWED,
        ),
        state(
            "wide-loaded-search-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("open-search"),
            WHOLE,
        ),
        state(
            "minimum-loaded-search-dark-200pct",
            480.,
            360.,
            DARK,
            200,
            Some("open-search"),
            REFLOWED,
        ),
        state(
            "wide-transcript-gap-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("open-transcript-gap"),
            WHOLE,
        ),
        state(
            "minimum-transcript-gap-dark-200pct",
            480.,
            360.,
            DARK,
            200,
            Some("open-transcript-gap"),
            REFLOWED,
        ),
        state(
            "daemon-tools-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("open-daemon-tools"),
            WHOLE,
        ),
        state(
            "minimum-daemon-tools-dark-200pct",
            480.,
            360.,
            DARK,
            200,
            Some("open-daemon-tools"),
            REFLOWED,
        ),
        state(
            "daemon-metrics-light",
            1440.,
            900.,
            LIGHT,
            100,
            Some("open-daemon-metrics"),
            WHOLE,
        ),
        state(
            "minimum-daemon-metrics-dark-200pct",
            480.,
            360.,
            DARK,
            200,
            Some("open-daemon-metrics"),
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

    fn inside(window: &Window, id: impl Into<ElementId>, viewport: Size<Pixels>) -> bool {
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
            viewer,
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
                let blocks = if viewer {
                    // Wide fallback glyphs, joined emoji, combining marks,
                    // bidi, tabs, long lines and CRLF. Copy stays verbatim.
                    let text = [
                        "漢字".repeat(200),
                        "👩🏽‍💻🇹🇭".repeat(100),
                        "ก้่ำ e\u{301} ".repeat(100),
                        "العربية עברית ".repeat(100),
                        "a\tb\t".repeat(100),
                    ]
                    .join("\r\n");
                    vec![TimelineBlock::new(41, BlockKind::ToolSummary, text)]
                } else {
                    fixture::timeline(40)
                };
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
                            inside(window, *id, viewport),
                            "{name}: with focus in the {label}, {id} is at {:?}, not inside the window",
                            window.find(*id).bounds()
                        );
                    }
                })
                .expect("offscreen window is open");
                if click.is_none() && !viewer {
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

        if viewer {
            cx.update_window(any, |_, window, cx| {
                shell
                    .update(cx, |shell, cx| shell.view_block(41, window, cx))
                    .unwrap();
                window.render_frame(cx);
            })
            .expect("offscreen window is open");
            cx.run_until_parked();
            cx.update_window(any, |_, window, cx| {
                window.render_frame(cx);
                check_viewer(name, &shell, window, cx, false);
            })
            .expect("offscreen window is open");
            capture(&mut cx, "-start");
            cx.update_window(any, |_, window, cx| {
                window.press("end", cx);
                window.render_frame(cx);
                window.render_frame(cx);
                check_viewer(name, &shell, window, cx, true);
            })
            .expect("offscreen window is open");
            capture(&mut cx, "-end");
        } else if let Some(control) = click {
            cx.update_window(any, |_, window, cx| {
                window.render_frame(cx);
                if control.starts_with("open-profile-questions") {
                    use formless_desktop::{client::Update,questions::{Open,State}};
                    let mut question=Open::from_event(&fixture::user_question("another-profile-session")).unwrap();
                    question.questions[0].question="A long question ไทย 漢字 😀: choose one option after reviewing the full prompt. ".repeat(20);
                    question.questions[0].options[0].label="Long option ไทย 漢字 😀 ".repeat(10);
                    question.questions[0].options[0].description=Some("Long option description for scrolling and focus checks. ".repeat(15));
                    let mut questions=vec![question.clone()];
                    if control=="open-profile-questions" {
                        for i in 1..8 { let mut question=question.clone();question.id=format!("native-question-{i}");questions.push(question); }
                    }
                    shell.update(cx,|shell,cx|{
                        shell.connect_recording_daemon(cx);
                        shell.apply_client_update(Update::Connection{ready:true,notice:"Connected serialized view fixture".into()},window,cx);
                        shell.apply_client_update(Update::Questions(State{open:questions,..State::default()}),window,cx);
                        shell.run_command(Command::Questions,window,cx);
                    });
                    window.render_frame(cx);
                    if control.ends_with("-form") {
                        for _ in 0..20 {
                            window.render_frame(cx);
                            if window.find("open-question-native-question").focused()==Some(true) { break; }
                            window.press("tab",cx);
                        }
                        for _ in 0..3 { window.render_frame(cx);window.simulate_next_frame(cx); }
                        window.click("open-question-native-question",cx);window.render_frame(cx);
                    }
                } else if matches!(control, "open-run-recovery" | "open-run-forecast") {
                    use formless_desktop::client::Update;
                    let sessions = fixture::sessions(3);
                    shell.update(cx, |shell, cx| {
                        shell.connect_recording_daemon(cx);
                        shell.apply_client_update(Update::Connection { ready: true, notice: "Connected".into() }, window, cx);
                        shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx);
                    });
                    window.render_frame(cx);
                    if window.try_find(shell::session_row_id(&sessions[0].id)).is_none() {
                        window.click("toggle-sidebar", cx);
                        window.render_frame(cx);
                    }
                    window.click(shell::session_row_id(&sessions[0].id), cx);
                    let generation = shell.read(cx).client_generation();
                    let snapshot = formless_desktop::replies::session_snapshot(&sessions[0].id,
                        &serde_json::json!({"session":{"id":sessions[0].id},"running":false,"messages":[],
                            "unfinished_run": if control == "open-run-recovery" { serde_json::json!({"run_id":"interrupted-run","status":"interrupted","error":"The provider request was stopped."}) } else { serde_json::Value::Null }})).unwrap();
                    shell.update(cx, |shell, cx| {
                        shell.apply_client_update(Update::Snapshot { generation, snapshot }, window, cx);
                        shell.focus_section(PageSection::Composer, window, cx);
                    });
                    if control == "open-run-forecast" {
                        let prepared = {
                            let state = shell.read(cx);
                            let session_id = sessions[0].id.clone();
                            let mut quote = fixture::run_forecast(&session_id, true);
                            quote.warnings = vec!["Forecast warning ไทย 漢字 😀. Estimates are uncertain.".repeat(10); 12];
                            Box::new(formless_desktop::forecast::Prepared {
                                id: uuid::Uuid::new_v4(),
                                epoch: state.client_epoch().unwrap(),
                                generation,
                                session_id,
                                label: "Send",
                                draft_basis: state.draft(cx).to_string(),
                                quote,
                            })
                        };
                        shell.update(cx, |shell, cx| shell.apply_client_update(Update::Prepared(prepared), window, cx));
                    }
                } else if control.starts_with("open-daemon-") {
                    use formless_desktop::client::Update;
                    let destination = if control == "open-daemon-metrics" { Destination::Metrics } else { Destination::Tools };
                    let value = if destination == Destination::Metrics { serde_json::json!({"summary":{"recorded_runs":1,"prompt_tokens":10,"completion_tokens":20}, "runs":[{"run_id":"recorded-run","status":"completed","duration_ms":1250}]}) } else { serde_json::json!([
                        {"id":"tool-a","name":"Tool ไทย 漢字 😀","description":"Unicode data from the daemon view boundary"},
                        {"id":"tool-b","name":"Second Tool","schema":{"type":"object"}}
                    ]) };
                    shell.update(cx, |shell, cx| {
                        shell.connect_recording_daemon(cx);
                        shell.apply_client_update(Update::Connection { ready: true, notice: "Connected · canonical messages · live token streaming unavailable".into() }, window, cx);
                        shell.apply_client_update(Update::Page { destination, result: Ok(value) }, window, cx);
                        shell.run_command(Command::GoTo(destination), window, cx);
                        shell.focus_section(PageSection::Conversation, window, cx);
                    });
                } else if control == "open-directory" {
                    shell.update(cx, |shell, cx| {
                        shell.run_command(Command::GoTo(Destination::Sessions), window, cx)
                    });
                } else if control == "open-search" {
                    shell.update(cx, |shell, cx| {
                        shell.run_command(Command::GoTo(Destination::Search), window, cx)
                    });
                    let query = shell.read(cx).search_query().clone();
                    query.update(cx, |query, cx| query.set_value("ข้อความ", window, cx));
                } else if control == "open-transcript-gap" {
                    let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
                    let snapshot = formless_desktop::replies::session_snapshot(
                        &id,
                        &serde_json::json!({
                            "session":{"id":id},"running":false,"messages":[]
                        }),
                    )
                    .unwrap();
                    shell.update(cx, |shell, cx| {
                        shell.show_session_snapshot(snapshot, cx).unwrap();
                        shell
                            .apply_transcript_event(
                                &formless_protocol::Event::DeltasDropped {
                                    session_id: id,
                                    run_id: String::new(),
                                    kinds: vec![],
                                    handle: None,
                                    model: None,
                                },
                                cx,
                            )
                            .unwrap();
                        shell.focus_section(PageSection::Conversation, window, cx);
                    });
                } else {
                    window.click(control, cx);
                }
            })
            .expect("offscreen window is open");
            cx.run_until_parked();
            if control.starts_with("open-profile-questions") {
                cx.update_window(any, |_, window, cx| {
                    window.render_frame(cx);
                    assert!(
                        inside(window, "questions-dialog", viewport),
                        "{name}: dialog outside viewport"
                    );
                    assert!(
                        inside(window, "questions-close", viewport),
                        "{name}: Close outside viewport"
                    );
                    assert_eq!(window.find("questions-close").focused(), Some(true));
                    if control.ends_with("-form") {
                        assert!(
                            inside(window, "question-submit", viewport),
                            "{name}: Submit outside viewport"
                        );
                    }
                })
                .expect("offscreen window is open");
                capture(&mut cx, "");
                let targets: Vec<(ElementId, bool)> = if control.ends_with("-form") {
                    cx.update_window(any, |_, window, cx| {
                        let top = window.find("question-prompt").bounds().top();
                        window.press("end", cx);
                        window.render_frame(cx);
                        window.render_frame(cx);
                        assert!(
                            window.find("question-prompt").bounds().top() < top,
                            "{name}: long question cannot be read with keys"
                        );
                        window.press("home", cx);
                        window.render_frame(cx);
                    })
                    .expect("offscreen window is open");
                    vec![
                        (ElementId::from(("question-option", 0usize)), false),
                        (ElementId::from(("question-option", 1usize)), false),
                        (ElementId::from("question-input-anchor"), true),
                        (ElementId::from("question-copy"), false),
                        (ElementId::from("question-prompt-copy"), false),
                        (ElementId::from("question-next"), false),
                    ]
                } else {
                    vec![(ElementId::from("open-question-native-question-7"), false)]
                };
                for (id, input) in targets {
                    cx.update_window(any, |_, window, cx| {
                        for _ in 0..80 {
                            window.render_frame(cx);
                            let focused = if input {
                                shell
                                    .read(cx)
                                    .question_answer_input()
                                    .unwrap()
                                    .focus_handle(cx)
                                    .is_focused(window)
                            } else {
                                window.find(id.clone()).focused() == Some(true)
                            };
                            if focused {
                                if input {
                                    let input = shell.read(cx).question_answer_input().unwrap();
                                    input.update(cx, |input, cx| {
                                        input.set_value(
                                            "one ไทย\ntwo 漢字\nthree 😀\nfour\nfive\nsix",
                                            window,
                                            cx,
                                        )
                                    });
                                }
                                break;
                            }
                            window.press("tab", cx);
                        }
                        for _ in 0..3 {
                            window.render_frame(cx);
                            window.simulate_next_frame(cx);
                        }
                    })
                    .expect("offscreen window is open");
                    cx.run_until_parked();
                    cx.update_window(any, |_, window, cx| {
                        window.render_frame(cx);
                        let focused = if input {
                            shell
                                .read(cx)
                                .question_answer_input()
                                .unwrap()
                                .focus_handle(cx)
                                .is_focused(window)
                        } else {
                            window.find(id.clone()).focused() == Some(true)
                        };
                        assert!(focused, "{name}: {id:?} is keyboard unreachable");
                        assert!(
                            inside(window, id.clone(), viewport),
                            "{name}: focused {id:?} outside viewport: {:?}",
                            window.find(id.clone()).bounds()
                        );
                        if input {
                            let field = window.find(id.clone()).bounds();
                            let body = window.find("questions-body").bounds();
                            assert!(
                                field.top() >= body.top() && field.bottom() <= body.bottom(),
                                "{name}: input is clipped by the dialog body"
                            );
                        }
                        if id == ElementId::from(("question-option", 0usize)) {
                            window.press("space", cx);
                            window.render_frame(cx);
                        }
                    })
                    .expect("offscreen window is open");
                }
                if control.ends_with("-form") {
                    cx.update_window(any, |_, window, cx| {
                        window.click("question-submit", cx);
                        window.render_frame(cx);
                        assert_eq!(window.find("question-submit").disabled(), Some(true));
                        assert_eq!(window.find("questions-close").focused(), Some(true));
                        assert!(
                            inside(window, "question-submit", viewport),
                            "{name}: waiting Submit outside viewport"
                        );
                        assert!(
                            inside(window, "questions-close", viewport),
                            "{name}: waiting Close outside viewport"
                        );
                    })
                    .expect("offscreen window is open");
                    capture(&mut cx, "-waiting");
                    cx.update_window(any, |_, window, cx| {
                        window.click("questions-close", cx);
                        window.render_frame(cx);
                        window.click("show-questions", cx);
                        window.render_frame(cx);
                        for _ in 0..30 {
                            if window.find("questions-copy-saved").focused() == Some(true) {
                                break;
                            }
                            window.press("tab", cx);
                            window.render_frame(cx);
                        }
                        for _ in 0..3 {
                            window.render_frame(cx);
                            window.simulate_next_frame(cx);
                        }
                    })
                    .expect("offscreen window is open");
                    cx.run_until_parked();
                    cx.update_window(any, |_, window, cx| {
                        window.render_frame(cx);
                        assert_eq!(window.find("questions-copy-saved").focused(), Some(true));
                        assert!(
                            inside(window, "questions-copy-saved", viewport),
                            "{name}: saved answer copy is outside viewport"
                        );
                        window.click("questions-copy-saved", cx);
                        let saved: serde_json::Value = serde_json::from_str(
                            &cx.read_from_clipboard().unwrap().text().unwrap(),
                        )
                        .unwrap();
                        assert_eq!(
                            saved[0]["answers"][0]["selected"][0],
                            "Long option ไทย 漢字 😀 ".repeat(10)
                        );
                    })
                    .expect("offscreen window is open");
                    capture(&mut cx, "-saved");
                }
            }
            if control == "open-run-forecast" {
                cx.update_window(any, |_, window, cx| {
                    window.render_frame(cx);
                    assert_eq!(window.find("forecast-cancel").focused(), Some(true));
                    for id in [
                        "forecast-dialog",
                        "forecast-cancel",
                        "forecast-copy",
                        "forecast-confirm",
                    ] {
                        assert!(
                            inside(window, id, viewport),
                            "{name}: {id} outside viewport: {:?}",
                            window.find(id).bounds()
                        );
                    }
                    let first = window.find("forecast-detail-0").bounds();
                    window.press("end", cx);
                    window.render_frame(cx);
                    assert!(
                        window.find("forecast-detail-0").bounds().top() < first.top(),
                        "{name}: forecast cannot be read with keys"
                    );
                    window.press("home", cx);
                    window.render_frame(cx);
                })
                .expect("offscreen window is open");
                capture(&mut cx, "");
                cx.update_window(any, |_, window, cx| {
                    window.click("forecast-confirm", cx);
                    window.render_frame(cx);
                    assert_eq!(window.find("forecast-confirm").disabled(), Some(true));
                    for id in [
                        "forecast-dialog",
                        "forecast-cancel",
                        "forecast-copy",
                        "forecast-confirm",
                    ] {
                        assert!(
                            inside(window, id, viewport),
                            "{name}: waiting {id} outside viewport"
                        );
                    }
                })
                .expect("offscreen window is open");
                capture(&mut cx, "-waiting");
            }
            if control == "open-run-recovery" {
                for id in ["run-resume", "run-abandon"] {
                    cx.update_window(any, |_, window, cx| {
                        for _ in 0..40 {
                            window.render_frame(cx);
                            if window.find(id).focused() == Some(true) {
                                break;
                            }
                            window.press("tab", cx);
                        }
                        // Native anchors apply on the requested next frame.
                        window.render_frame(cx);
                        window.simulate_next_frame(cx);
                        window.render_frame(cx);
                        window.simulate_next_frame(cx);
                    })
                    .expect("offscreen window is open");
                    cx.run_until_parked();
                    cx.update_window(any, |_, window, cx| {
                        window.render_frame(cx);
                        assert_eq!(
                            window.find(id).focused(),
                            Some(true),
                            "{name}: {id} is keyboard unreachable"
                        );
                        assert!(
                            inside(window, id, viewport),
                            "{name}: focused {id} outside viewport: {:?}",
                            window.find(id).bounds()
                        );
                        assert!(
                            window.find(id).bounds().top() >= px(4.),
                            "{name}: {id} leaves no room for its focus ring"
                        );
                    })
                    .expect("offscreen window is open");
                }
            }
            if control.starts_with("open-daemon-") {
                cx.update_window(any, |_, window, cx| {
                    window.render_frame(cx);
                    for id in ["refresh-resource", "resource-back"] {
                        assert!(
                            inside(window, id, viewport),
                            "{name}: {id} outside the viewport: {:?}",
                            window.find(id).bounds()
                        );
                    }
                })
                .expect("offscreen window is open");
            }
            if fit == Fit::Reflowed
                && !control.starts_with("open-daemon-")
                && control != "open-run-recovery"
                && control != "open-run-forecast"
                && !control.starts_with("open-profile-questions")
            {
                // The overlay panel is inside the window, edge to edge at
                // most, whatever the page under it was scrolled to.
                cx.update_window(any, |_, window, cx| {
                    window.render_frame(cx);
                    let panel = match control {
                        "toggle-sidebar" => "region-sidebar",
                        "show-commands" => "palette",
                        "open-directory" | "open-search" | "open-transcript-gap" => {
                            "region-conversation"
                        }
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
            if control != "open-run-forecast" && !control.starts_with("open-profile-questions") {
                capture(&mut cx, "");
            }
        } else if fit == Fit::Whole {
            capture(&mut cx, "");
        }
        captures
    }

    /// Actual font advances of the visible rows fit the list, not just a
    /// count of graphemes. These assertions still need a working native
    /// offscreen runtime; compiling them is not a real-font pass.
    fn check_viewer(name: &str, shell: &Entity<Shell>, window: &mut Window, cx: &App, end: bool) {
        let count = shell.read(cx).viewer_rows().expect("viewer is open");
        assert!(count > 1, "{name}: no fitted rows");
        let dialog = window.find("viewer").bounds();
        let available = dialog.size.width - px(48.);
        let mut style = window.text_style();
        style.font_family = cx.theme().mono_font_family.clone();
        let size = cx.theme().mono_font_size;
        let first_or_last = if end { count - 1 } else { 0 };
        assert!(
            window.try_find(("viewer-row", first_or_last)).is_some(),
            "{name}: expected end row not visible"
        );
        for index in (0..256).chain(std::iter::once(count - 1)) {
            let Some(query) = window.try_find(("viewer-row", index)) else {
                continue;
            };
            let text = query.value().expect("row has plain text").to_string();
            let measured = window
                .text_system()
                .shape_line(text.clone().into(), size, &[style.to_run(text.len())], None)
                .width();
            assert!(
                measured <= available,
                "{name}: row {index} is {measured:?}, only {available:?} available"
            );
            let bounds = query.bounds();
            assert!(
                bounds.left() >= dialog.left() && bounds.right() <= dialog.right(),
                "{name}: row outside dialog"
            );
        }
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
