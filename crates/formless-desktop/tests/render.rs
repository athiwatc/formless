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
        px, size, AppContext as _, Bounds, HeadlessAppContext, WindowBounds, WindowOptions,
    };

    /// Name, window width, theme, text size in percent, and a control to
    /// click before capturing.
    const STATES: [(&str, f32, ThemeMode, u16, Option<&str>); 10] = [
        ("wide-light", 1440., ThemeMode::Light, 100, None),
        ("wide-dark", 1440., ThemeMode::Dark, 100, None),
        ("medium-light", 1100., ThemeMode::Light, 100, None),
        ("narrow-dark", 800., ThemeMode::Dark, 100, None),
        (
            "medium-inspector-overlay-light",
            1100.,
            ThemeMode::Light,
            100,
            Some("toggle-inspector"),
        ),
        (
            "narrow-sidebar-overlay-dark",
            800.,
            ThemeMode::Dark,
            100,
            Some("toggle-sidebar"),
        ),
        (
            "wide-tools-scaffold-light",
            1440.,
            ThemeMode::Light,
            100,
            Some("destination-Tools"),
        ),
        ("wide-light-200pct", 1440., ThemeMode::Light, 200, None),
        (
            "wide-sidebar-overlay-light-200pct",
            1440.,
            ThemeMode::Light,
            200,
            Some("toggle-sidebar"),
        ),
        ("minimum-dark-200pct", 480., ThemeMode::Dark, 200, None),
    ];

    pub fn run() {
        let keep = std::env::var("FORMLESS_DESKTOP_RENDER_DIR").ok();
        for (name, width, mode, percent, click) in STATES {
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
                            // The last state is the binary's minimum window.
                            size: size(px(width), px(if width < 500. { 360. } else { 900. })),
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
