//! Headless GPUI Kit tests of the Phase 0 shell.
//!
//! These drive the real `Shell` view in the kit's test window: real hit
//! testing, key dispatch, focus and layout. They do not prove pixels, OS IME
//! behaviour, a screen reader, or anything about a Daemon Core.

use formless_desktop::fixture;
use formless_desktop::model::{tokens, Destination, Intent};
use formless_desktop::shell::{self, session_row_id, Shell};
use formless_protocol::SendMode;
use gpui_kit::base::test_support::texts;
use gpui_kit::base::Root;
use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    px, size, App, AppContext as _, Bounds, Entity, Pixels, Point, Role, Size, TestAppContext,
    Window, WindowBounds, WindowHandle, WindowOptions,
};

const WIDE: Size<Pixels> = Size {
    width: px(1440.),
    height: px(900.),
};

fn open(
    cx: &mut TestAppContext,
    window_size: Size<Pixels>,
    sessions: usize,
    blocks: usize,
) -> (WindowHandle<Root>, Entity<Shell>) {
    open_with(cx, window_size, sessions, blocks, true)
}

/// `with_running: false` leaves every fixture Session idle, which changes
/// how the sidebar groups and orders them.
fn open_with(
    cx: &mut TestAppContext,
    window_size: Size<Pixels>,
    sessions: usize,
    blocks: usize,
    with_running: bool,
) -> (WindowHandle<Root>, Entity<Shell>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        shell::bind_keys(cx);
        let sessions = fixture::sessions(sessions);
        let running = if with_running {
            fixture::running(&sessions)
        } else {
            Default::default()
        };
        let blocks = fixture::timeline(blocks);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: window_size,
            })),
            ..Default::default()
        };
        let (window, shell) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Shell::new(&sessions, &running, blocks, window, cx))
        })
        .expect("open test window");
        (window.downcast::<Root>().expect("Base Root"), shell)
    })
}

#[cfg(target_os = "macos")]
const TOGGLE_SIDEBAR: &str = "cmd-b";
#[cfg(not(target_os = "macos"))]
const TOGGLE_SIDEBAR: &str = "ctrl-b";
#[cfg(target_os = "macos")]
const TOGGLE_INSPECTOR: &str = "cmd-alt-b";
#[cfg(not(target_os = "macos"))]
const TOGGLE_INSPECTOR: &str = "ctrl-alt-b";

/// One window update. Events a control emits (a composer Enter, for one) are
/// delivered when the update ends, so an assertion about their effect belongs
/// in the next step.
fn step<R>(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    update: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(handle.into(), |_, window, cx| update(window, cx))
        .expect("shell window is open")
}

#[gpui_kit::test]
fn wide_window_docks_three_panes_at_their_token_widths(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let sidebar = window.find("region-sidebar").bounds();
        let conversation = window.find("region-conversation").bounds();
        let inspector = window.find("region-inspector").bounds();
        assert_eq!(sidebar.size.width, px(tokens::SIDEBAR_DEFAULT));
        assert_eq!(inspector.size.width, px(tokens::INSPECTOR_DEFAULT));
        assert_eq!(sidebar.left(), px(0.));
        assert_eq!(conversation.left(), sidebar.right());
        assert_eq!(inspector.left(), conversation.right());
        assert_eq!(inspector.right(), px(1440.));
        assert!(window.find("header").visible());
        assert!(window.find("status").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn narrowing_hides_the_inspector_until_asked_then_collapses_the_sidebar(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);

    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Narrowing alone puts nothing over the conversation or its controls.
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.find("region-sidebar").visible());
        assert_eq!(
            window.find("region-conversation").bounds().right(),
            px(1100.)
        );

        // Asked for, it comes up as an overlay: the conversation keeps its
        // width and the inspector covers its right edge.
        window.click("toggle-inspector", cx);
        let conversation = window.find("region-conversation").bounds();
        let inspector = window.find("region-inspector").bounds();
        assert_eq!(conversation.right(), px(1100.));
        assert_eq!(inspector.right(), px(1100.));
        assert!(inspector.left() < conversation.right());
        // The control that dismisses it stays outside the overlay.
        let toggle = window.find("toggle-inspector").bounds();
        assert!(toggle.bottom() <= inspector.top());
        window.click("toggle-inspector", cx);
        assert!(window.try_find("region-inspector").is_none());
    });

    cx.simulate_window_resize(handle.into(), size(px(800.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("region-sidebar").is_none());
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(window.find("region-conversation").bounds().left(), px(0.));
        assert!(window.find("composer-primary").visible());
    });

    // Widening restores the docked inspector: that preference never changed.
    cx.simulate_window_resize(handle.into(), WIDE);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("region-inspector").visible());
        assert!(window.find("region-sidebar").visible());
    });
}

#[gpui_kit::test]
fn composer_takes_thai_cjk_and_emoji_and_keeps_the_draft_after_a_recorded_send(
    cx: &mut TestAppContext,
) {
    let text = "สวัสดีครับ 日本語のテスト 한국어 👩🏽‍💻";
    let draft = format!("{text}\nsecond line");
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The composer has focus from the start, so typing needs no click.
        let composer = window.find_by_label("Message");
        assert_eq!(composer.focused(), Some(true));
        assert_eq!(composer.role(), Some(Role::MultilineTextInput));
        // Nothing to send yet.
        window.press("enter", cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).last_intent(), None);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");

        window.input(text, cx);
        assert_eq!(window.find_by_label("Message").value(), Some(text));
        window.press("shift-enter", cx);
        window.input("second line", cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
        assert_eq!(shell.read(cx).last_intent(), None);
        window.press("enter", cx);
    });
    step(cx, handle, |_, cx| {
        assert_eq!(
            shell.read(cx).last_intent(),
            Some(&Intent::SendMessage {
                // The pinned fixture Session sorts first and opens at start.
                session_id: "fixture-session-3".to_string(),
                content: draft.clone(),
                mode: SendMode::Send,
            })
        );
        // Nothing acknowledged the request, so the draft is still there.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
    });
}

#[gpui_kit::test]
fn a_running_session_offers_steer_queue_and_a_separate_stop(cx: &mut TestAppContext) {
    #[cfg(target_os = "macos")]
    const SECONDARY_ENTER: &str = "cmd-enter";
    #[cfg(not(target_os = "macos"))]
    const SECONDARY_ENTER: &str = "ctrl-enter";

    let (handle, shell) = open(cx, WIDE, 200, 40);
    let last_mode = |cx: &App| match shell.read(cx).last_intent() {
        Some(Intent::SendMessage { mode, .. }) => Some(*mode),
        _ => None,
    };
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Idle Session: Send only, Stop present but unavailable.
        assert_eq!(window.find("composer-primary").label(), Some("Send"));
        assert!(window.try_find("composer-secondary").is_none());
        let stop = window.find("composer-stop").bounds();
        window.click("composer-stop", cx);
        assert_eq!(shell.read(cx).last_intent(), None);

        // Fixture Session 7 is running.
        window.click(session_row_id("fixture-session-7"), cx);
        assert_eq!(window.find("composer-primary").label(), Some("Steer"));
        assert_eq!(window.find("composer-secondary").label(), Some("Queue"));
        // Stop did not move when the primary action changed.
        assert_eq!(window.find("composer-stop").bounds(), stop);

        window.click("composer-stop", cx);
        assert_eq!(
            shell.read(cx).last_intent(),
            Some(&Intent::InterruptRun {
                session_id: "fixture-session-7".to_string()
            })
        );

        // With no draft the send controls do nothing.
        window.click("composer-primary", cx);
        window.click("composer-secondary", cx);
        assert_eq!(last_mode(cx), None);

        window.click_at("region-composer", gpui_kit::point(px(40.), px(20.)), cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.input("แก้ทิศทาง", cx);
        window.click("composer-secondary", cx);
        assert_eq!(last_mode(cx), Some(SendMode::Queue));
        window.click("composer-primary", cx);
        assert_eq!(last_mode(cx), Some(SendMode::Steer));

        window.click_at("region-composer", gpui_kit::point(px(40.), px(20.)), cx);
        window.press(SECONDARY_ENTER, cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(last_mode(cx), Some(SendMode::Queue));
        window.press("enter", cx);
    });
    step(cx, handle, |_, cx| {
        assert_eq!(last_mode(cx), Some(SendMode::Steer));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "แก้ทิศทาง");
    });
}

#[gpui_kit::test]
fn f6_moves_between_regions_and_skips_a_closed_panel(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let focused = |window: &gpui_kit::Window, id: &'static str| window.find(id).focused();

        // Composer -> Context -> Sessions -> Conversation -> Composer.
        window.press("f6", cx);
        assert_eq!(focused(window, "region-inspector"), Some(true));
        window.press("f6", cx);
        assert_eq!(focused(window, "region-sidebar"), Some(true));
        window.press("f6", cx);
        assert_eq!(focused(window, "region-conversation"), Some(true));
        window.press("f6", cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.press("shift-f6", cx);
        assert_eq!(focused(window, "region-conversation"), Some(true));

        // Closing the inspector removes it from the cycle.
        window.click("toggle-inspector", cx);
        assert!(window.try_find("region-inspector").is_none());
        window.press("shift-f6", cx);
        assert_eq!(focused(window, "region-sidebar"), Some(true));
        window.press("shift-f6", cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn closing_a_focused_panel_returns_focus_to_the_composer(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("f6", cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        window.press(TOGGLE_INSPECTOR, cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn arrows_preview_a_session_and_only_enter_opens_it(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.input("unsent draft", cx);
        window.press("f6", cx);
        window.press("f6", cx);
        assert_eq!(window.find("region-sidebar").focused(), Some(true));
        let opened = shell.read(cx).sidebar().active_id().map(str::to_string);

        window.press("down", cx);
        window.press("down", cx);
        assert_eq!(shell.read(cx).sidebar().selected_index(), Some(2));
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        // Previewing did not touch the draft or steal focus.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent draft");
        assert_eq!(window.find("region-sidebar").focused(), Some(true));

        window.press("enter", cx);
        let second = shell.read(cx).sidebar().rows()[2].id.clone();
        assert_eq!(shell.read(cx).sidebar().active_id(), Some(second.as_str()));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        assert_eq!(window.find("region-sidebar").focused(), Some(true));

        // Going back restores the first Session's draft.
        window.press("up", cx);
        window.press("up", cx);
        window.press("enter", cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent draft");
    })
    .unwrap();
}

#[gpui_kit::test]
fn ten_thousand_sessions_and_blocks_materialize_only_what_is_near_the_viewport(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 10_000, 10_000);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let rows = shell.read(cx).sidebar().rows().len();
        assert!(rows > 9_000, "archived Sessions are filtered: {rows}");

        let observed = window.elements();
        let count = |name: &str| {
            observed
                .iter()
                .filter(|element| {
                    element
                        .path()
                        .last()
                        .is_some_and(|id| format!("{id:?}").contains(name))
                })
                .count()
        };
        let session_rows = count("session-row");
        let blocks = count("block");
        // Rows are identified by Session id, not by position.
        let row_id = |index: usize| session_row_id(&shell.read(cx).sidebar().rows()[index].id);
        assert!(window.find(row_id(0)).visible());
        assert!(window.try_find(row_id(5_000)).is_none());
        assert!(
            (1..=60).contains(&session_rows),
            "session rows: {session_rows}"
        );

        // The timeline follows its tail: the newest block is there, the
        // oldest was never built.
        assert!(window.find(("block", 10_000usize)).visible());
        assert!(window.try_find(("block", 1usize)).is_none());
        assert!((1..=60).contains(&blocks), "timeline blocks: {blocks}");
    })
    .unwrap();
}

#[gpui_kit::test]
fn markdown_paints_thai_cjk_and_right_to_left_text(cx: &mut TestAppContext) {
    // Tall enough for all eight fixture bodies at once.
    let (handle, _) = open(cx, size(px(1440.), px(2400.)), 20, 8);
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        let painted: String = texts(window)
            .iter()
            .filter(|text| text.visible())
            .map(|text| text.text().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "ช่วยสรุปค่าใช้จ่ายของเดือนนี้ให้หน่อย",
            "了解しました。以下に要点をまとめます。",
            "收入",
            "items.iter().sum() // 合計 / ผลรวม",
            "تقرير.pdf",
            "👩🏽‍💻",
        ] {
            assert!(
                painted.contains(expected),
                "{expected:?} was not painted:\n{painted}"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_long_unspaced_thai_paragraph_wraps_inside_a_narrow_conversation(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, size(px(520.), px(2400.)), 20, 8);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        let conversation = window.find("region-conversation").bounds();
        assert_eq!(conversation.size.width, px(520.));
        let painted = texts(window);
        let paragraph = painted
            .iter()
            .find(|text| text.text().starts_with("ข้อความอ้างอิงภาษาไทย"))
            .expect("the Thai quote was painted");
        let one_line = painted
            .iter()
            .find(|text| text.text() == "block 8")
            .expect("a short single-line Markdown paragraph was painted")
            .bounds()
            .size
            .height;
        // It breaks onto further lines instead of overflowing or clipping.
        // Where it breaks is not asserted: GPUI 0.3.12 has no Thai
        // word-break model, so the break is not at a word boundary.
        assert!(paragraph.bounds().size.height > one_line * 1.5);
        assert!(paragraph.bounds().left() >= conversation.left());
        assert!(paragraph.bounds().right() <= conversation.right());
        assert!(paragraph.visible());
    });
}

#[gpui_kit::test]
fn a_session_row_keeps_its_element_id_at_a_different_position(cx: &mut TestAppContext) {
    // The same Sessions listed twice: with Session 7 running it sorts into
    // the Active group, idle it stays in Recent. The row is at a different
    // position in each list and is found under the same element id.
    let mut position = |with_running: bool| {
        // Tall enough that the row is materialized in either list.
        let (handle, shell) = open_with(cx, size(px(1440.), px(2400.)), 200, 8, with_running);
        step(cx, handle, |window, cx| {
            window.render_frame(cx);
            let index = shell
                .read(cx)
                .sidebar()
                .rows()
                .iter()
                .position(|row| row.id == "fixture-session-7")
                .expect("fixture session 7");
            let top = window
                .find(session_row_id("fixture-session-7"))
                .bounds()
                .top();
            (index, top)
        })
    };
    let (running_index, running_top) = position(true);
    let (idle_index, idle_top) = position(false);
    assert_ne!(running_index, idle_index);
    assert_ne!(running_top, idle_top);
}

#[gpui_kit::test]
fn the_inspector_overlay_is_modal_traps_focus_and_restores_it(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("draft under the overlay", cx);
        // Put focus somewhere other than the default before opening.
        window.press("shift-f6", cx);
        assert_eq!(window.find("region-conversation").focused(), Some(true));

        window.press(TOGGLE_INSPECTOR, cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        assert!(window.find("overlay-backdrop").visible());

        // F6 and Tab cannot leave the overlay.
        for key in ["f6", "shift-f6", "tab", "shift-tab", "tab"] {
            window.press(key, cx);
            assert_eq!(
                window.find("region-inspector").focused(),
                Some(true),
                "{key} left the overlay"
            );
        }

        // Send sits under the overlay panel: a click there reaches the panel,
        // not the button.
        let send = window.find("composer-primary").bounds();
        let inspector = window.find("region-inspector").bounds();
        assert!(send.right() > inspector.left());
        window.click("composer-primary", cx);
        assert_eq!(shell.read(cx).last_intent(), None);
        assert!(window.find("region-inspector").visible());

        // Escape dismisses it and focus goes back where it was.
        window.press("escape", cx);
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.try_find("overlay-backdrop").is_none());
        assert_eq!(window.find("region-conversation").focused(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft under the overlay");

        // With the overlay gone the same click works.
        window.click("composer-primary", cx);
        assert!(shell.read(cx).last_intent().is_some());
    });
}

#[gpui_kit::test]
fn a_click_on_the_backdrop_dismisses_the_overlay_and_nothing_else(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(TOGGLE_INSPECTOR, cx);
        assert!(window.find("region-inspector").visible());
        let opened = shell.read(cx).sidebar().active_id().map(str::to_string);

        // The backdrop's top-left corner lies over the first Session row.
        window.click_at("overlay-backdrop", gpui_kit::point(px(40.), px(60.)), cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn the_sessions_toggle_opens_a_modal_sidebar_when_too_narrow_to_dock(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(800.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("region-sidebar").is_none());
        window.input("keep me", cx);

        window.press(TOGGLE_SIDEBAR, cx);
        let sidebar = window.find("region-sidebar");
        assert_eq!(sidebar.focused(), Some(true));
        assert_eq!(sidebar.bounds().left(), px(0.));
        // It lies over the conversation, which keeps the full width.
        assert_eq!(window.find("region-conversation").bounds().left(), px(0.));
        assert!(window.find("overlay-backdrop").visible());

        // Arrows preview inside the overlay; Escape closes without opening.
        let opened = shell.read(cx).sidebar().active_id().map(str::to_string);
        window.press("down", cx);
        window.press("escape", cx);
        assert!(window.try_find("region-sidebar").is_none());
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));

        // Enter opens the selected Session, closes the overlay and returns
        // focus to the composer it came from.
        window.press(TOGGLE_SIDEBAR, cx);
        window.press("down", cx);
        window.press("enter", cx);
        let selected = shell.read(cx).sidebar().rows()[2].id.clone();
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(selected.as_str())
        );
        assert!(window.try_find("region-sidebar").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");

        // The header toggle works too, and closes what it opened.
        window.click("toggle-sidebar", cx);
        assert!(window.find("region-sidebar").visible());
        window.click("toggle-sidebar", cx);
        assert!(window.try_find("region-sidebar").is_none());
    });
}

#[gpui_kit::test]
fn widening_docks_an_open_overlay_and_narrowing_again_does_not_reopen_it(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(TOGGLE_INSPECTOR, cx);
        assert!(window.find("overlay-backdrop").visible());
    });

    cx.simulate_window_resize(handle.into(), WIDE);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Docked now: no backdrop, and focus is back in a visible region.
        assert!(window.try_find("overlay-backdrop").is_none());
        assert!(window.find("region-inspector").visible());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });

    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.try_find("overlay-backdrop").is_none());
    });
}

#[gpui_kit::test]
fn other_destinations_show_a_scaffold_notice_instead_of_the_conversation(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("still here", cx);

        window.click("destination-Tools", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Tools);
        // No conversation, no composer: nothing that looks like a Tools page.
        assert!(window.try_find("region-composer").is_none());
        assert!(window.try_find("composer-primary").is_none());
        assert!(window.try_find(("block", 40usize)).is_none());
        assert!(window.find("back-to-chat").visible());
        // Focus left the composer for the region that replaced it.
        assert_eq!(window.find("region-conversation").focused(), Some(true));

        window.draw(cx).clear(cx);
        let painted: Vec<String> = texts(window)
            .iter()
            .filter(|text| text.visible())
            .map(|text| text.text().to_string())
            .collect();
        assert!(
            painted.iter().any(|text| text == "Tools: not built"),
            "{painted:?}"
        );
        assert!(painted
            .iter()
            .any(|text| text.contains("This Phase 0 scaffold only has a Chat view")));

        // The region cycle skips the composer that is not there.
        window.press("f6", cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        window.press("f6", cx);
        assert_eq!(window.find("region-sidebar").focused(), Some(true));
        window.press("f6", cx);
        assert_eq!(window.find("region-conversation").focused(), Some(true));

        window.click("back-to-chat", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert!(window.find("region-composer").visible());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "still here");
    });
}

/// Press Tab until `id` holds keyboard focus.
fn tab_to(window: &mut Window, id: &'static str, cx: &mut App) {
    for _ in 0..60 {
        if window.find(id).focused() == Some(true) {
            return;
        }
        window.press("tab", cx);
    }
    panic!("Tab never reached {id}");
}

#[gpui_kit::test]
fn panels_expose_landmark_roles_docked_and_named_dialogs_as_overlays(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let sidebar = window.find("region-sidebar");
        assert_eq!(sidebar.role(), Some(Role::Navigation));
        assert_eq!(sidebar.label(), Some("Sessions"));
        assert_eq!(sidebar.description(), None);
        let inspector = window.find("region-inspector");
        assert_eq!(inspector.role(), Some(Role::Complementary));
        assert_eq!(inspector.label(), Some("Context"));
    });

    cx.simulate_window_resize(handle.into(), size(px(800.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(TOGGLE_INSPECTOR, cx);
        let inspector = window.find("region-inspector");
        assert_eq!(inspector.role(), Some(Role::Dialog));
        assert_eq!(inspector.label(), Some("Context"));
        assert!(inspector
            .description()
            .is_some_and(|text| text.contains("Escape")));
        // Exactly one dialog is exposed while an overlay is up.
        assert_eq!(window.find_all_by_role(Role::Dialog).len(), 1);

        window.press(TOGGLE_SIDEBAR, cx);
        let sidebar = window.find("region-sidebar");
        assert_eq!(sidebar.role(), Some(Role::Dialog));
        assert_eq!(sidebar.label(), Some("Sessions"));
        assert_eq!(window.find_all_by_role(Role::Dialog).len(), 1);

        window.press("escape", cx);
        assert!(window.find_all_by_role(Role::Dialog).is_empty());
    });
}

#[gpui_kit::test]
fn dismissing_an_overlay_returns_focus_to_the_header_control_that_opened_it(
    cx: &mut TestAppContext,
) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        tab_to(window, "toggle-inspector", cx);
        // Activate the focused toggle from the keyboard.
        window.press("enter", cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        assert_eq!(window.find("toggle-inspector").focused(), Some(false));

        window.press("escape", cx);
        assert!(window.try_find("region-inspector").is_none());
        // Back on the very button, not on the composer or a neighbour.
        assert_eq!(window.find("toggle-inspector").focused(), Some(true));
        assert_eq!(window.find("toggle-sidebar").focused(), Some(false));
        assert_eq!(window.find_by_label("Message").focused(), Some(false));

        // The same button closes it again, and focus stays on that button.
        window.press("enter", cx);
        assert!(window.find("region-inspector").visible());
        window.click_at("overlay-backdrop", gpui_kit::point(px(40.), px(60.)), cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(window.find("toggle-inspector").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn dismissing_an_overlay_returns_focus_to_the_control_inside_a_region(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("destination-Tools", cx);
    });
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        tab_to(window, "back-to-chat", cx);

        window.press(TOGGLE_INSPECTOR, cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        assert_eq!(window.find("back-to-chat").focused(), Some(false));
        window.press("escape", cx);
        // `focused()` on a region means "contains focus", so the button, a
        // leaf, is what shows that focus came back to the control itself.
        assert_eq!(window.find("back-to-chat").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn focus_falls_back_when_the_originating_control_is_gone(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(800.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Opened from the composer, then a destination without a composer is
        // chosen inside the overlay: the composer cannot take focus back.
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.press(TOGGLE_SIDEBAR, cx);
        window.click("destination-Metrics", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Metrics);
        assert!(window.try_find("region-sidebar").is_none());
        assert!(window.try_find("region-composer").is_none());
        assert_eq!(window.find("region-conversation").focused(), Some(true));
    });
}
