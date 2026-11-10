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
    use formless_desktop::shell::{self, Shell};
    use gpui_kit::assets::Assets;
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        px, size, AppContext as _, Bounds, HeadlessAppContext, WindowBounds, WindowOptions,
    };

    /// Name, window width, theme, and a control to click before capturing.
    const STATES: [(&str, f32, ThemeMode, Option<&str>); 7] = [
        ("wide-light", 1440., ThemeMode::Light, None),
        ("wide-dark", 1440., ThemeMode::Dark, None),
        ("medium-light", 1100., ThemeMode::Light, None),
        ("narrow-dark", 800., ThemeMode::Dark, None),
        (
            "medium-inspector-overlay-light",
            1100.,
            ThemeMode::Light,
            Some("toggle-inspector"),
        ),
        (
            "narrow-sidebar-overlay-dark",
            800.,
            ThemeMode::Dark,
            Some("toggle-sidebar"),
        ),
        (
            "wide-tools-scaffold-light",
            1440.,
            ThemeMode::Light,
            Some("destination-Tools"),
        ),
    ];

    pub fn run() {
        let keep = std::env::var("FORMLESS_DESKTOP_RENDER_DIR").ok();
        for (name, width, mode, click) in STATES {
            let mut cx = HeadlessAppContext::with_platform(
                gpui_kit::platform::current_platform(true).text_system(),
                Arc::new(Assets),
                gpui_kit::platform::current_headless_renderer,
            );
            let (handle, _) = cx
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
                            size: size(px(width), px(900.)),
                        })),
                        focus: false,
                        show: false,
                        ..Default::default()
                    };
                    gpui_kit::open_window(options, cx, |window, cx| {
                        cx.new(|cx| Shell::new(&sessions, &running, blocks, window, cx))
                    })
                })
                .expect("open offscreen window");
            cx.run_until_parked();
            if let Some(control) = click {
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.click(control, cx);
                })
                .expect("offscreen window is open");
                cx.run_until_parked();
            }
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
    }
}
